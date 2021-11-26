package endpoints

import (
	"fmt"
	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/ag-computational-bio/bakta-web-backend/argoclient"
	"github.com/ag-computational-bio/bakta-web-backend/objectStorage"
	"github.com/spf13/viper"
	"google.golang.org/grpc"
	"log"
	"net"
)

//RunGrpcJobServer Runs the Grpc server
func RunGrpcJobServer() error {

	namespace := viper.GetString("K8sNamespace")
	if len(namespace) == 0 {
		log.Fatalln("param K8sNamespace is required")
	}
	wfTemplate := viper.GetString("WorkflowTemplate")

	if len(wfTemplate) == 0 {
		log.Fatalln("param WorkflowTemplate is required")
	}

	bucket := viper.GetString("Objectstorage.S3.Bucket")

	if len(bucket) == 0 {
		log.Fatalln("param Objectstorage.S3.Bucket is required")
	}

	endpoint := viper.GetString("Objectstorage.S3.Endpoint")

	if len(endpoint) == 0 {
		log.Fatalln("Objectstorage.S3.Endpoint is required")
	}

	aClient := argoclient.NewClient(namespace, wfTemplate)
	statusHandler := argoclient.NewStatusHandler(aClient)

	statusHandler.Run()

	s3Handler, err := objectStorage.InitS3ObjectStorageHandler(bucket, endpoint)
	if err != nil {
		log.Println(fmt.Sprintf("failed to listen: %v", err))
		return err
	}

	listener, err := net.Listen("tcp", ":8080")
	if err != nil {
		log.Println(fmt.Sprintf("failed to listen: %v", err))
		return err
	}
	s := grpc.NewServer()

	baktaJobsEndpoints := InitBaktaAPI(statusHandler, s3Handler)

	api.RegisterBaktaJobsServer(s, baktaJobsEndpoints)
	go func() {
		log.Fatalln(s.Serve(listener))
	}()

	log.Println("Started grpc server")

	return nil
}
