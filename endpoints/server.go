package endpoints

import (
	"context"
	"fmt"
	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/ag-computational-bio/bakta-web-backend/argoclient"
	"github.com/ag-computational-bio/bakta-web-backend/objectStorage"
	"github.com/spf13/viper"
	"golang.org/x/sync/errgroup"
	"google.golang.org/grpc"
	"log"
	"net"
)

//RunGrpcJobServer Runs the Grpc server
func RunGrpcJobServer() error {

	namespace := viper.GetString("K8sNamespace")
	wfTemplate := viper.GetString("WorkflowTemplate")

	bucket := viper.GetString("Objectstorage.S3.Bucket")
	endpoint := viper.GetString("Objectstorage.S3.Endpoint")

	aClient := argoclient.NewClient(namespace, wfTemplate)
	statusHandler := argoclient.NewStatusHandler(aClient)

	statusHandler.Run()

	s3Handler, err := objectStorage.InitS3ObjectStorageHandler(bucket, endpoint)
	if err != nil {
		log.Println(fmt.Sprintf("failed to listen: %v", err))
		return err
	}

	jobServer := initGrpcJobServer(statusHandler, s3Handler)

	serverErrGrp, _ := errgroup.WithContext(context.Background())

	jobListener, err := net.Listen("tcp", fmt.Sprintf("localhost:%v", "8080"))
	if err != nil {
		log.Println(fmt.Sprintf("failed to listen: %v", err))
		return err
	}

	serverErrGrp.Go(func() error {
		return jobServer.Serve(jobListener)
	})

	log.Println("Started grpc server")

	return serverErrGrp.Wait()
}

// initGrpcServer Initializes a new GRPC server that handles bakta-web-api endpoints
func initGrpcJobServer(statusHandler *argoclient.StatusHandler, s3Handler *objectStorage.S3ObjectStorageHandler) *grpc.Server {
	baktaJobsEndpoints := InitBaktaAPI(statusHandler, s3Handler)

	var opts []grpc.ServerOption
	//opts = append(opts, grpc.UnaryInterceptor(authHandler.unaryInterceptor))

	grpcServer := grpc.NewServer(opts...)
	api.RegisterBaktaJobsServer(grpcServer, baktaJobsEndpoints)

	return grpcServer
}
