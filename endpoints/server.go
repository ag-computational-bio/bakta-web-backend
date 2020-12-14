package endpoints

import (
	"context"
	"fmt"
	"log"
	"net"
	"os"

	"github.com/ag-computational-bio/bakta-web-backend/scheduler"

	"github.com/ag-computational-bio/bakta-web-backend/database"

	"github.com/ag-computational-bio/bakta-web-api/go/api"
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

	dbHandler, err := database.InitDatabaseHandler(database.Postgres)
	if err != nil {
		log.Fatalln(err.Error())
	}

	namespace := "bakta-test"

	sched, err := scheduler.InitSimpleScheduler(namespace)
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

	jobServer := initGrpcJobServer(dbHandler, sched, authHandler)
	updateServer := initGrpcUpdateServer(authHandler)

	serverErrGrp, _ := errgroup.WithContext(context.Background())

	serverErrGrp.Go(func() error {
		return jobServer.Serve(jobListener)
	})

	serverErrGrp.Go(func() error {
		return updateServer.Serve(updateListener)
	})

	return serverErrGrp.Wait()
}

// initGrpcServer Initializes a new GRPC server that handles bakta-web-api endpoints
func initGrpcJobServer(dbHandler *database.Handler, sched *scheduler.SimpleScheduler, authHandler *AuthHandler) *grpc.Server {
	baktaJobsEndpoints := InitBaktaAPI(dbHandler, sched)

	var opts []grpc.ServerOption
	opts = append(opts, grpc.UnaryInterceptor(authHandler.unaryInterceptor))

	grpcServer := grpc.NewServer(opts...)
	api.RegisterBaktaJobsServer(grpcServer, baktaJobsEndpoints)

	return grpcServer
}

// initGrpcServer Initializes a new GRPC server that handles bakta-web-api endpoints
func initGrpcUpdateServer(authHandler *AuthHandler) *grpc.Server {

	baktaUpdateEndpoints := InitUpdateAPI()

	var opts []grpc.ServerOption
	opts = append(opts, grpc.UnaryInterceptor(authHandler.unaryInterceptor))

	grpcServer := grpc.NewServer()
	api.RegisterBaktaStatusUpdateServer(grpcServer, baktaUpdateEndpoints)

	return grpcServer
}

// unaryInterceptor calls authenticateClient with current context
func (authHandler *AuthHandler) unaryInterceptor(ctx context.Context, req interface{}, info *grpc.UnaryServerInfo, handler grpc.UnaryHandler) (interface{}, error) {
	if md, ok := metadata.FromIncomingContext(ctx); ok {
		if token, ok := md["X-API-Token"]; ok {
			if len(token) == 1 && token[0] == authHandler.token {
				handler(ctx, req)
			}
		}
	}

	return "", fmt.Errorf("missing credentials")
}
