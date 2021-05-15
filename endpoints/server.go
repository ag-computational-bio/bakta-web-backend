package endpoints

import (
	"context"
	"fmt"
	"log"
	"net"
	"os"

	"github.com/ag-computational-bio/bakta-web-api-go/api"
	"github.com/ag-computational-bio/bakta-web-backend/monitor"
	"github.com/ag-computational-bio/bakta-web-backend/objectStorage"
	"github.com/ag-computational-bio/bakta-web-backend/scheduler"
	"github.com/spf13/viper"

	"github.com/ag-computational-bio/bakta-web-backend/database"

	"golang.org/x/sync/errgroup"
	"google.golang.org/grpc"
	"google.golang.org/grpc/metadata"
)

//AuthHandler Simple auth handler to check if a grpc call has a token
//Only for testing purposes
type AuthHandler struct {
	token string
}

//InitAuthHandler Initiates a simple auth handler
func InitAuthHandler() (*AuthHandler, error) {
	apiToken := os.Getenv("ApiToken")
	if apiToken == "" {
		return nil, fmt.Errorf("Invalid API Token")
	}

	authHandler := AuthHandler{
		token: apiToken,
	}

	return &authHandler, nil
}

//RunGrpcJobServer Runs the Grpc server
func RunGrpcJobServer() error {
	authHandler, err := InitAuthHandler()
	if err != nil {
		log.Fatalln(err.Error())
	}

	dbHandler, err := database.InitDatabaseHandler()
	if err != nil {
		log.Fatalln(err.Error())
	}

	sched, err := scheduler.InitSimpleScheduler(dbHandler)
	if err != nil {
		log.Fatalln(err.Error())
	}

	jobListener, err := net.Listen("tcp", fmt.Sprintf("localhost:%v", "8080"))
	if err != nil {
		log.Println(fmt.Sprintf("failed to listen: %v", err))
		return err
	}

	updateListener, err := net.Listen("tcp", fmt.Sprintf("localhost:%v", "8081"))
	if err != nil {
		log.Println(fmt.Sprintf("failed to listen: %v", err))
		return err
	}

	bucket := viper.GetString("Objectstorage.S3.Bucket")

	s3Handler, err := objectStorage.InitS3ObjectStorageHandler(bucket)
	if err != nil {
		log.Println(fmt.Sprintf("failed to listen: %v", err))
		return err
	}

	updateMonitor := monitor.New(sched.GetK8sClient(), sched.GetNamespace())

	jobServer := initGrpcJobServer(dbHandler, sched, authHandler, s3Handler, &updateMonitor)

	updateHandler := BaktaUpdateAPI{
		dbHandler:     dbHandler,
		updateMonitor: &updateMonitor,
	}

	updateServer := initGrpcUpdateServer(&updateHandler)

	serverErrGrp, _ := errgroup.WithContext(context.Background())

	serverErrGrp.Go(func() error {
		return jobServer.Serve(jobListener)
	})

	serverErrGrp.Go(func() error {
		return updateServer.Serve(updateListener)
	})

	log.Println("Started grpc server")

	return serverErrGrp.Wait()
}

// initGrpcServer Initializes a new GRPC server that handles bakta-web-api endpoints
func initGrpcJobServer(dbHandler *database.Handler, sched *scheduler.SimpleScheduler, authHandler *AuthHandler, s3Handler *objectStorage.S3ObjectStorageHandler, updateMonitor *monitor.SimpleMonitor) *grpc.Server {
	baktaJobsEndpoints := InitBaktaAPI(dbHandler, sched, s3Handler, updateMonitor)

	var opts []grpc.ServerOption
	opts = append(opts, grpc.UnaryInterceptor(authHandler.unaryInterceptor))

	grpcServer := grpc.NewServer(opts...)
	api.RegisterBaktaJobsServer(grpcServer, baktaJobsEndpoints)

	return grpcServer
}

// initGrpcServer Initializes a new GRPC server that handles bakta-web-api endpoints
func initGrpcUpdateServer(baktaUpdateEndpoints *BaktaUpdateAPI) *grpc.Server {
	grpcServer := grpc.NewServer()
	api.RegisterBaktaStatusUpdateServer(grpcServer, baktaUpdateEndpoints)

	return grpcServer
}

// unaryInterceptor calls authenticateClient with current context
func (authHandler *AuthHandler) unaryInterceptor(ctx context.Context, req interface{}, info *grpc.UnaryServerInfo, handler grpc.UnaryHandler) (interface{}, error) {
	if md, ok := metadata.FromIncomingContext(ctx); ok {
		if token, ok := md["authorization"]; ok {
			if len(token) == 1 && token[0] == authHandler.token {
				return handler(ctx, req)
			}
			err := fmt.Errorf("API key does not match")
			log.Println(err.Error())
			return "", err
		}
	}

	err := fmt.Errorf("error authenticating credentials")
	log.Println(err.Error())
	return "", err
}
